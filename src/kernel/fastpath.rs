use crate::{
    config::{
        msgRegister, seL4_Fault_NullFault, seL4_MsgExtraCapBits, seL4_MsgLengthBits, tcbCTable,
        tcbCaller, tcbReply, tcbVTable, EPState_Idle, EPState_Recv, EPState_Send,
        NtfnState_Active, SysCall, SysReplyRecv, ThreadStateBlockedOnReceive,
        ThreadStateBlockedOnReply, ThreadStateRunning,
    },
    object::{
        structure_gen::{
            endpoint_ptr_get_epQueue_head,
            endpoint_ptr_get_epQueue_tail, endpoint_ptr_get_state, notification_ptr_get_state, 
            seL4_Fault_get_seL4_FaultType, thread_state_get_blockingIPCCanGrant, thread_state_set_blockingIPCCanGrant,
        },
        tcb::isHighestPrio,
    },
    println,
    structures::{
        endpoint_t, pte_t, seL4_MessageInfo_t, tcb_t, thread_state_t,
    },
    MASK,
};
use core::intrinsics::{likely, unlikely};
use common::sel4_config::wordBits;
use cspace::interface::*;

use super::{
    c_traps::slowpath,
    thread::{getCSpace, getRegister, ksCurThread, setRegister},
    transfermsg::{
        messageInfoFromWord_raw, seL4_MessageInfo_ptr_get_length,
        seL4_MessageInfo_ptr_set_capsUnwrapped, wordFromMessageInfo,
    },
    vspace::{pptr_to_paddr, setVSpaceRoot},
};
#[inline]
#[no_mangle]
pub fn lookup_fp(_cap: &cap_t, cptr: usize) -> cap_t {
    let mut cap = _cap.clone();
    let mut bits = 0;
    let mut guardBits: usize;
    let mut radixBits: usize;
    let mut cptr2: usize;
    let mut capGuard: usize;
    let mut radix: usize;
    let mut slot: *mut cte_t;
    if unlikely(!cap_capType_equals(&cap, cap_cnode_cap)) {
        return cap_null_cap_new();
    }
    loop {
        guardBits = cap_cnode_cap_get_capCNodeGuardSize(&cap);
        radixBits = cap_cnode_cap_get_capCNodeRadix(&cap);
        cptr2 = cptr << bits;
        capGuard = cap_cnode_cap_get_capCNodeGuard(&cap);
        if likely(guardBits != 0) && unlikely(cptr2 >> (wordBits - guardBits) != capGuard) {
            return cap_null_cap_new();
        }

        radix = cptr2 << guardBits >> (wordBits - radixBits);
        slot = unsafe { (cap_cnode_cap_get_capCNodePtr(&cap) as *mut cte_t).add(radix) };
        cap = unsafe { (*slot).cap };
        bits += guardBits + radixBits;

        if likely(!(bits < wordBits && cap_capType_equals(&cap, cap_cnode_cap))) {
            break;
        }
    }
    if bits > wordBits {
        return cap_null_cap_new();
    }
    return cap;
}

#[inline]
#[no_mangle]
pub fn thread_state_ptr_set_tsType_np(ts_ptr: &mut thread_state_t, tsType: usize) {
    ts_ptr.words[0] = tsType;
}

#[inline]
#[no_mangle]
pub fn thread_state_ptr_mset_blockingObject_tsType(
    ptr: &mut thread_state_t,
    ep: usize,
    tsType: usize,
) {
    (*ptr).words[0] = ep | tsType;
}

#[inline]
#[no_mangle]
pub fn cap_reply_cap_ptr_new_np(
    ptr: &mut cap_t,
    capReplyCanGrant: usize,
    capReplyMaster: usize,
    capTCBPtr: usize,
) {
    ptr.words[1] = capTCBPtr;
    ptr.words[0] = capReplyMaster | (capReplyCanGrant << 1) | (cap_reply_cap << 59);
}

#[inline]
#[no_mangle]
pub fn endpoint_ptr_mset_epQueue_tail_state(ptr: *mut endpoint_t, tail: usize, state: usize) {
    unsafe {
        (*ptr).words[0] = tail | state;
    }
}
#[inline]
#[no_mangle]
pub fn endpoint_ptr_set_epQueue_head_np(ptr: *mut endpoint_t, head: usize) {
    unsafe {
        (*ptr).words[1] = head;
    }
}

#[inline]
#[no_mangle]
pub fn switchToThread_fp(thread: *mut tcb_t, vroot: *mut pte_t, stored_hw_asid: pte_t) {
    let asid = stored_hw_asid.words[0];
    unsafe {
        setVSpaceRoot(pptr_to_paddr(vroot as usize), asid);
        ksCurThread = thread;
    }
}

#[inline]
#[no_mangle]
pub fn mdb_node_ptr_mset_mdbNext_mdbRevocable_mdbFirstBadged(
    ptr: &mut mdb_node_t,
    mdbNext: usize,
    mdbRevocable: usize,
    mdbFirstBadged: usize,
) {
    ptr.words[1] = mdbNext | (mdbRevocable << 1) | mdbFirstBadged;
}

#[inline]
#[no_mangle]
pub fn mdb_node_ptr_set_mdbPrev_np(ptr: &mut mdb_node_t, prev: usize) {
    ptr.words[0] = prev;
}

#[inline]
#[no_mangle]
pub fn isValidVTableRoot_fp(cap: &cap_t) -> bool {
    cap_capType_equals(cap, cap_page_table_cap) && cap_page_table_cap_get_capPTIsMapped(cap) != 0
}

#[inline]
#[no_mangle]
pub fn fastpath_mi_check(msgInfo: usize) -> bool {
    (msgInfo & MASK!(seL4_MsgLengthBits + seL4_MsgExtraCapBits)) > 4
}

#[inline]
#[no_mangle]
pub fn fastpath_copy_mrs(length: usize, src: *mut tcb_t, dest: *mut tcb_t) {
    let mut reg: usize;
    for i in 0..length {
        reg = msgRegister[0] + i;
        setRegister(dest, reg, getRegister(src, reg));
        if getRegister(src, reg) != getRegister(dest, reg) {
            println!("wrong!!!!");
        }
    }
}

#[inline]
#[no_mangle]
pub fn fastpath_reply_cap_check(cap: &cap_t) -> bool {
    cap_capType_equals(cap, cap_reply_cap)
}

core::arch::global_asm!(include_str!("restore_fp.S"));

#[inline]
#[no_mangle]
pub fn fastpath_restore(badge: usize, msgInfo: usize, cur_thread: *mut tcb_t) {
    let cur_thread_regs = unsafe { (*cur_thread).tcbArch.registers.as_ptr() as usize };
    extern "C" {
        pub fn __restore_fp(badge: usize, msgInfo: usize, cur_thread_reg: usize);
    }
    unsafe {
        __restore_fp(badge, msgInfo, cur_thread_regs);
    }
    // println!("msgInfo:{}",msgInfo);
    // unsafe {
    //     asm!("mv a0,x12",in("x12") badge);
    //     asm!("mv a1,x12",in("x12") msgInfo);
    //     asm!("mv t0,x12",in("x12") cur_thread_regs);
    //     asm!(
    //         "ld  ra, (0*8)(t0)  ",
    //         "ld  sp, (1*8)(t0)  ",
    //         "ld  gp, (2*8)(t0)  ",
    //         "ld  t2, (6*8)(t0)  ",
    //         "ld  s0, (7*8)(t0)  ",
    //         "ld  s1, (8*8)(t0)  ",
    //         "ld  a2, (11*8)(t0) ",
    //         "ld  a3, (12*8)(t0) ",
    //         "ld  a4, (13*8)(t0) ",
    //         "ld  a5, (14*8)(t0) ",
    //         "ld  a6, (15*8)(t0) ",
    //         "ld  a7, (16*8)(t0) ",
    //         "ld  s2, (17*8)(t0) ",
    //         "ld  s3, (18*8)(t0) ",
    //         "ld  s4, (19*8)(t0) ",
    //         "ld  s5, (20*8)(t0) ",
    //         "ld  s6, (21*8)(t0) ",
    //         "ld  s7, (22*8)(t0) ",
    //         "ld  s8, (23*8)(t0) ",
    //         "ld  s9, (24*8)(t0) ",
    //         "ld  s10, (25*8)(t0)",
    //         "ld  s11, (26*8)(t0)",
    //         "ld  t3, (27*8)(t0) ",
    //         "ld  t4, (28*8)(t0) ",
    //         "ld  t5, (29*8)(t0) ",
    //         "ld  t6, (30*8)(t0) ",
    //         "ld  t1, (3*8)(t0)  ",
    //         "add tp, t1, x0  ",
    //         "ld  t1, (34*8)(t0)",
    //         "csrw sepc, t1  ",
    //         "csrw sscratch, t0",
    //         "ld  t1, (32*8)(t0) ",
    //         "csrw sstatus, t1",
    //         "ld  t1, (5*8)(t0)",
    //         "ld  t0, (4*8)(t0) ",
    //         "sret"
    //     );
    // }
}

// #[no_mangle]
// pub fn process5(
//     badge: usize,
//     msgInfo: usize,
//     ep_ptr: *mut endpoint_t,
//     cptr: usize,
//     cap_pd: *mut pte_t,
//     callerSlot: *mut cte_t,
//     replySlot: *mut cte_t,
//     replyCanGrant: usize,
//     dest: *mut tcb_t,
//     w: usize,
//     length:usize,
// ) {
//     unsafe {
//         // let mut info: seL4_MessageInfo_t = messageInfoFromWord_raw(msgInfo);
//         // let length = seL4_MessageInfo_ptr_get_length((&info) as *const seL4_MessageInfo_t);
//         // let ep_slot = unsafe { getCSpace(ksCurThread as usize, tcbCTable) };
//         // let ep_cap = unsafe { lookup_fp(&(*ep_slot).cap, cptr) };
//         // let dest = endpoint_ptr_get_epQueue_head(ep_ptr) as *mut tcb_t;

//         // if unlikely(endpoint_ptr_get_state(ep_ptr) != EPState_Recv) {
//         //     slowpath(SysCall as usize);
//         // }
//         // let newVTable = unsafe { &(*getCSpace(dest as usize, tcbVTable)).cap };

//         // // let cap_pd = cap_page_table_cap_get_capPTBasePtr(newVTable) as *mut pte_t;

//         // if unlikely(!isValidVTableRoot_fp(newVTable)) {
//         //     slowpath(SysCall as usize);
//         // }

//         // let mut stored_hw_asid: pte_t = pte_t { words: [0] };
//         // stored_hw_asid.words[0] = cap_page_table_cap_get_capPTMappedASID(newVTable);

//         // let dom = 0;
//         // unsafe {
//         //     if unlikely(
//         //         (*dest).tcbPriority < (*ksCurThread).tcbPriority
//         //             && !isHighestPrio(dom, (*dest).tcbPriority),
//         //     ) {
//         //         slowpath(SysCall as usize);
//         //     }
//         // }
//         // if unlikely(
//         //     (cap_endpoint_cap_get_capCanGrant(&ep_cap) == 0)
//         //         && (cap_endpoint_cap_get_capCanGrantReply(&ep_cap) == 0),
//         // ) {
//         //     slowpath(SysCall as usize);
//         // }
//         // unsafe {
//         //     endpoint_ptr_set_epQueue_head_np(ep_ptr, (*dest).tcbEPNext);
//         //     if unlikely((*dest).tcbEPNext != 0) {
//         //         (*((*dest).tcbEPNext as *mut tcb_t)).tcbEPPrev = 0;
//         //     } else {
//         //         endpoint_ptr_mset_epQueue_tail_state(ep_ptr, 0, EPState_Idle);
//         //     }
//         // }

//         // let badge = cap_endpoint_cap_get_capEPBadge(&ep_cap);
//         // unsafe {
//         //     thread_state_ptr_set_tsType_np(&mut (*ksCurThread).tcbState, ThreadStateBlockedOnReply);
//         // }

//         // let replySlot = unsafe { getCSpace(ksCurThread as usize, tcbReply) };

//         // let callerSlot = getCSpace(dest as usize, tcbCaller);

//         // let replyCanGrant = unsafe { thread_state_get_blockingIPCCanGrant(&(*dest).tcbState) };
//         let mut stored_hw_asid: pte_t = pte_t { words: [w] };
//         unsafe {
//             // cap_reply_cap_ptr_new_np(
//             //     &mut (*callerSlot).cap,
//             //     replyCanGrant,
//             //     0,
//             //     ksCurThread as usize,
//             // );
//             // mdb_node_ptr_set_mdbPrev_np(&mut (*callerSlot).cteMDBNode, replySlot as usize);
//             // mdb_node_ptr_mset_mdbNext_mdbRevocable_mdbFirstBadged(
//             //     &mut (*replySlot).cteMDBNode,
//             //     callerSlot as usize,
//             //     1,
//             //     1,
//             // );
//             fastpath_copy_mrs(length, ksCurThread, dest);
//             thread_state_ptr_set_tsType_np(&mut (*dest).tcbState, ThreadStateRunning);
//             switchToThread_fp(dest, cap_pd, stored_hw_asid);
//         }
//         let mut info: seL4_MessageInfo_t = messageInfoFromWord_raw(msgInfo);
//         seL4_MessageInfo_ptr_set_capsUnwrapped((&mut info) as *mut seL4_MessageInfo_t, 0);
//         let msgInfo1 = wordFromMessageInfo(info);
//         // println!("badge :{:#x} msgInfo:{:#x} ksCurThread:{:#x},dest:{:#x},cap_pd:{:#x},stored_hw_asid:{:#x}",badge,msgInfo);
//         fastpath_restore(badge, msgInfo1, ksCurThread);
//     }
// }

#[inline]
#[no_mangle]
pub fn fastpath_call(cptr: usize, msgInfo: usize) {
    // slowpath(SysCall as usize);
    let mut info: seL4_MessageInfo_t = messageInfoFromWord_raw(msgInfo);
    let length = seL4_MessageInfo_ptr_get_length((&info) as *const seL4_MessageInfo_t);
    let fault_type = unsafe { seL4_Fault_get_seL4_FaultType(&(*ksCurThread).tcbFault) };

    if fastpath_mi_check(msgInfo) || fault_type != seL4_Fault_NullFault {
        slowpath(SysCall as usize);
    }
    let ep_slot = unsafe { getCSpace(ksCurThread as usize, tcbCTable) };
    let ep_cap = unsafe { lookup_fp(&(*ep_slot).cap, cptr) };
    if unlikely(
        !cap_capType_equals(&ep_cap, cap_endpoint_cap)
            || (cap_endpoint_cap_get_capCanSend(&ep_cap) == 0),
    ) {
        slowpath(SysCall as usize);
    }
    let ep_ptr = cap_endpoint_cap_get_capEPPtr(&ep_cap) as *mut endpoint_t;

    let dest = endpoint_ptr_get_epQueue_head(ep_ptr) as *mut tcb_t;

    if unlikely(endpoint_ptr_get_state(ep_ptr) != EPState_Recv) {
        slowpath(SysCall as usize);
    }

    let newVTable = unsafe { &(*getCSpace(dest as usize, tcbVTable)).cap };

    let cap_pd = cap_page_table_cap_get_capPTBasePtr(newVTable) as *mut pte_t;

    if unlikely(!isValidVTableRoot_fp(newVTable)) {
        slowpath(SysCall as usize);
    }

    let mut stored_hw_asid: pte_t = pte_t { words: [0] };
    stored_hw_asid.words[0] = cap_page_table_cap_get_capPTMappedASID(newVTable);

    let dom = 0;
    unsafe {
        if unlikely(
            (*dest).tcbPriority < (*ksCurThread).tcbPriority
                && !isHighestPrio(dom, (*dest).tcbPriority),
        ) {
            slowpath(SysCall as usize);
        }
    }
    if unlikely(
        (cap_endpoint_cap_get_capCanGrant(&ep_cap) == 0)
            && (cap_endpoint_cap_get_capCanGrantReply(&ep_cap) == 0),
    ) {
        slowpath(SysCall as usize);
    }
    unsafe {
        endpoint_ptr_set_epQueue_head_np(ep_ptr, (*dest).tcbEPNext);
        if unlikely((*dest).tcbEPNext != 0) {
            (*((*dest).tcbEPNext as *mut tcb_t)).tcbEPPrev = 0;
        } else {
            endpoint_ptr_mset_epQueue_tail_state(ep_ptr, 0, EPState_Idle);
        }
    }

    let badge = cap_endpoint_cap_get_capEPBadge(&ep_cap);
    unsafe {
        thread_state_ptr_set_tsType_np(&mut (*ksCurThread).tcbState, ThreadStateBlockedOnReply);
    }

    let replySlot = unsafe { getCSpace(ksCurThread as usize, tcbReply) };

    let callerSlot = getCSpace(dest as usize, tcbCaller);

    let replyCanGrant = unsafe { thread_state_get_blockingIPCCanGrant(&(*dest).tcbState) };
    unsafe {
        cap_reply_cap_ptr_new_np(
            &mut (*callerSlot).cap,
            replyCanGrant,
            0,
            ksCurThread as usize,
        );
        mdb_node_ptr_set_mdbPrev_np(&mut (*callerSlot).cteMDBNode, replySlot as usize);
        mdb_node_ptr_mset_mdbNext_mdbRevocable_mdbFirstBadged(
            &mut (*replySlot).cteMDBNode,
            callerSlot as usize,
            1,
            1,
        );
        fastpath_copy_mrs(length, ksCurThread, dest);
        thread_state_ptr_set_tsType_np(&mut (*dest).tcbState, ThreadStateRunning);
        switchToThread_fp(dest, cap_pd, stored_hw_asid);
        seL4_MessageInfo_ptr_set_capsUnwrapped((&mut info) as *mut seL4_MessageInfo_t, 0);
        let msgInfo1 = wordFromMessageInfo(info);
        // println!("badge :{:#x} msgInfo:{:#x} ksCurThread:{:#x},dest:{:#x},cap_pd:{:#x},stored_hw_asid:{:#x}",badge,msgInfo,ksCurThread as usize,dest as usize ,cap_pd as usize , stored_hw_asid.words[0]);
        fastpath_restore(badge, msgInfo1, ksCurThread);
    }
}

#[inline]
#[no_mangle]
pub fn fastpath_reply_recv(cptr: usize, msgInfo: usize) {
    // slowpath(SysReplyRecv as usize);
    let mut info = messageInfoFromWord_raw(msgInfo);
    let length = seL4_MessageInfo_ptr_get_length((&info) as *const seL4_MessageInfo_t);
    let mut fault_type = unsafe { seL4_Fault_get_seL4_FaultType(&(*ksCurThread).tcbFault) };

    if fastpath_mi_check(msgInfo) || fault_type != seL4_Fault_NullFault {
        slowpath(SysReplyRecv as usize);
    }
    let ep_slot = unsafe { getCSpace(ksCurThread as usize, tcbCTable) };
    let ep_cap = unsafe { lookup_fp(&(*ep_slot).cap, cptr) };

    if unlikely(
        !cap_capType_equals(&ep_cap, cap_endpoint_cap)
            || (cap_endpoint_cap_get_capCanSend(&ep_cap) == 0),
    ) {
        slowpath(SysReplyRecv as usize);
    }

    unsafe {
        if unlikely(
            (*ksCurThread).tcbBoundNotification as usize != 0
                && notification_ptr_get_state((*ksCurThread).tcbBoundNotification)
                    == NtfnState_Active,
        ) {
            slowpath(SysReplyRecv as usize);
        }
    }
    let ep_ptr = cap_endpoint_cap_get_capEPPtr(&ep_cap) as *mut endpoint_t;

    if unlikely(endpoint_ptr_get_state(ep_ptr) == EPState_Send) {
        slowpath(SysReplyRecv as usize);
    }

    let callerSlot = unsafe { getCSpace(ksCurThread as usize, tcbCaller) };
    let callerCap = unsafe { &(*callerSlot).cap };

    if unlikely(!fastpath_reply_cap_check(callerCap)) {
        slowpath(SysReplyRecv as usize);
    }

    let caller = cap_reply_cap_get_capTCBPtr(callerCap) as *mut tcb_t;

    fault_type = unsafe { seL4_Fault_get_seL4_FaultType(&(*caller).tcbFault) };

    if unlikely(fault_type != seL4_Fault_NullFault) {
        slowpath(SysReplyRecv as usize);
    }

    let newVTable = unsafe { &(*getCSpace(caller as usize, tcbVTable)).cap };

    let cap_pd = cap_page_table_cap_get_capPTBasePtr(newVTable) as *mut pte_t;

    if unlikely(!isValidVTableRoot_fp(newVTable)) {
        slowpath(SysReplyRecv as usize);
    }

    let mut stored_hw_asid: pte_t = pte_t { words: [0] };
    stored_hw_asid.words[0] = cap_page_table_cap_get_capPTMappedASID(newVTable);

    let dom = 0;

    unsafe {
        if unlikely(!isHighestPrio(dom, (*caller).tcbPriority)) {
            slowpath(SysReplyRecv as usize);
        }
        thread_state_ptr_mset_blockingObject_tsType(
            &mut (*ksCurThread).tcbState,
            ep_ptr as usize,
            ThreadStateBlockedOnReceive,
        );
        thread_state_set_blockingIPCCanGrant(
            &mut (*ksCurThread).tcbState,
            cap_endpoint_cap_get_capCanGrant(&ep_cap),
        );
    }

    let endpointTail = endpoint_ptr_get_epQueue_tail(ep_ptr) as *mut tcb_t;

    if endpointTail as usize == 0 {
        unsafe {
            (*ksCurThread).tcbEPPrev = 0;
            (*ksCurThread).tcbEPNext = 0;
            endpoint_ptr_set_epQueue_head_np(ep_ptr, ksCurThread as usize);
            endpoint_ptr_mset_epQueue_tail_state(ep_ptr, ksCurThread as usize, EPState_Recv);
        }
    } else {
        unsafe {
            (*endpointTail).tcbEPNext = ksCurThread as usize;
            (*ksCurThread).tcbEPPrev = endpointTail as usize;
            (*ksCurThread).tcbEPNext = 0;
            endpoint_ptr_mset_epQueue_tail_state(ep_ptr, ksCurThread as usize, EPState_Recv);
        }
    }

    unsafe {
        let node = mdb_node_get_mdbPrev(&(*callerSlot).cteMDBNode) as *mut cte_t;
        mdb_node_ptr_mset_mdbNext_mdbRevocable_mdbFirstBadged(&mut (*node).cteMDBNode, 0, 1, 1);
        (*callerSlot).cap = cap_null_cap_new();
        (*callerSlot).cteMDBNode = mdb_node_new(0, 0, 0, 0);
        fastpath_copy_mrs(length, ksCurThread, caller);

        thread_state_ptr_set_tsType_np(&mut (*caller).tcbState, ThreadStateRunning);
        switchToThread_fp(caller, cap_pd, stored_hw_asid);
        seL4_MessageInfo_ptr_set_capsUnwrapped((&mut info) as *mut seL4_MessageInfo_t, 0);
        let msgInfo1 = wordFromMessageInfo(info);
        // println!("out fastpath_reply_recv{}", msgInfo1);
        fastpath_restore(0, msgInfo1, ksCurThread);
    }
}
 